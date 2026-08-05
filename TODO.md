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
- **A new family whose name extends an existing one gets swallowed by prefix bucketing.**
  Several dispatch points bucket on `func.starts_with("ATTACK")` rather than on the exact name.
  B1 found two: the editor's `hitboxes_from_captures` read a captured `ATTACK_ABS` through
  `ATTACK`'s 36-slot table (sixteen arguments against thirty-six — it came back as a plausible,
  wrong hitbox), and the plugin's `is_collision_func` armed the clear-all gate for a call
  nothing clears. The *parser* was safe only because it matches `macros::NAME(` with the paren.
  Before adding such a family, grep for `starts_with(` on the prefix you are extending and fix
  every hit; then add the exact-name arm **above** the prefix one.
- **A walk that unrolls loops will count a line once per iteration.** `eval_effect_stmts` and
  `eval_stmts` both run a `for` body `count` times, which is right for the calls they produce
  and wrong for anything that names a *line of source*: a report, a site ordinal, a residue
  buffer. C6 read `COL_NORMAL` as 16 losses against a true 8 this way, and B4 hit the same
  thing with hurtbox site ordinals. The fix both times was to rewind the per-line state at the
  top of each iteration and step it once for the whole body — `call_macro_ordinals` and
  `EffectStmt::Loop` in `eval_effect_stmts` are the two worked examples. **The tell is a count
  that is an exact multiple of the truth**, so check a new number against `main` in a worktree
  before believing it; a plausible number is the easiest kind to ship.
- **A macro's name is not its family. Check `lua_const` before modelling it.** `COL_PRI` and
  `COL_NORMAL` read as body collision for four tasks running, and B4 modelled them as hurtbox
  statements on the strength of it. They are `MA_MSC_CMD_COLOR_BLEND_COL_PRI` and
  `MA_MSC_CMD_COLOR_BLEND_COL_NORMAL` — the `FLASH` family. Grepping `lua_const.rs` for the
  macro's name takes a second and gives you the `MA_MSC_CMD_<FAMILY>_` prefix the game itself
  files it under; that prefix is the answer to "which existing table does this belong in", and
  it is usually a table you already have. The cost of guessing is not just a wrong doc comment:
  it puts the control in the wrong panel and sends the fix down a new-type path when a one-row
  path existed. **When `lua_const` is silent, the signature is the next-best oracle** — it has
  no constant for any of B3's five macros, and it was `WHOLE_HIT`'s `(hit_status)` argument that
  showed it to be B4's family rather than the hitbox tuner its name and its entry both claimed.
- **Not every macro in an entry's list belongs to that entry's shape — check the signature of
  each, not just of the first.** B3 was framed as "modifiers attached to the hitbox id they
  target" and named four macros. Two of them (`ATK_HIT_ABS`, `ATK_LERP_RATIO`) take no id at
  all, so the framing could not describe them; they were not a smaller version of the task but a
  different one, and saying so in the entry was the deliverable. A grouping is a hypothesis until
  each member's `macros.rs` line is read. The tell that you are about to do this wrong is
  planning the work for a *list* rather than for a shape.
- **A shared numbering space is a coupling, and site ordinals are the place it bites.** Families
  that are edited by ordinal-into-source need their own counter each. Folding B3's modifiers into
  `HurtSite` would have meant adding an `ATK_POWER` to a move silently shifted every later
  `HIT_NODE`'s site, retuning a call the user never clicked — and no existing test would have
  failed, because each family passes its own tests in isolation. Test the boundary from *both*
  sides when adding a family beside an existing one.
- **The same trap again, and worse, across the wire: `Hitbox.category` is not the plugin's
  category.** They agree for attack (0), grab (1) and wind (2) and then diverge — the editor's
  `CAT_ABS` is 3, the plugin's is 4, because the plugin's space also carries hurtbox state,
  which took 3. B5 found that every live `ATTACK_ABS` edit since B1 had gone out as category 3
  and been read as a hurtbox rule: **silently dead, for two whole tasks.** Everything now goes
  through `game_link::wire_category()`. The general rule: *two id spaces that agree on their
  first few members are not the same space, and the agreement is exactly what hides it.* When
  adding a family, add its wire value to that function and to the test that pins the mapping.
- **The same fact needs a different test on each surface.** `SEARCH` and `CATCH` are each
  written with and without their capsule arguments, and every surface has to tell which. In
  source text the discriminator is the *shape of the token* — a coordinate versus a `*CONST`.
  On the live wire that test is wrong, because an int and a float are both just numbers there,
  and it happily read a 14-argument call's collision kind, hit status and undocumented int as a
  capsule of `[2.0, 1.0, 60.0]`. There the discriminator is the argument *count*. Do not carry
  a guard across surfaces without asking what signal that surface actually has.
- **A round-trip oracle cannot see a value lost on the way in.** `check_hitbox_fidelity`
  compares the export against the *parsed model*. Anything the parser drops or defaults agrees
  with itself on the way out, so the check is green — which is how four short-form `CATCH`
  calls sat in the corpus for the whole project reading as ordinary grabs instead of Kirby's
  swallow. When a parse change is the thing at issue, **assert against the original text.**
- **A parser that "preserves unknown lines verbatim" still has to preserve their braces.**
  `parse_stmts` skipped every lone `}` and kept an `if …{` as a one-line `Raw`, so a runtime
  branch was flattened: the closing brace vanished and both arms of an `if`/`else` were promoted
  to unconditional. **35 of 236 `game_` scripts in the corpus exported a function with more `{`
  than `}`** — a mod that does not build — and the round-trip oracles all passed, because they
  re-parse the export with the same lenient parser that wrote it, and an unbalanced function
  reads back exactly as it went in. The general rule: *a "verbatim" escape hatch that is a
  single line cannot represent a construct that spans lines.* `AcmdStmt::RawBlock` is the shape
  that can. When adding one, check what a `}` on its own line currently does.
- **Verbatim round-trip is the wrong bar when the source itself is malformed.** Six corpus
  `sound_` scripts are mis-indented — the dumper does not re-indent after an `else` — so a
  byte-equality gate fails on a *dumper* bug and pressures you into reproducing it. Assert the
  three properties that actually matter instead: the trimmed lines are the same (nothing lost),
  emitting twice is a fixed point (a rewritten file does not drift), and the byte-exact count is
  pinned (formatting cannot rot unnoticed). See
  `every_sound_script_in_the_corpus_survives_a_round_trip`.
- **A bullet written from memory is not a measurement, even when you wrote it.** C6b's
  `CANCEL_FILL_SCREEN` item claimed C3 modelled `FILL_SCREEN_MODEL_COLOR` and that the reset was
  a `COLOR_COMMANDS`-shaped row. C3 does not, and it is not — C6's own result table three
  hundred lines up had already recorded "a different family", and the real arities are 12 and 2.
  Both errors were caught by the arity check the bullet itself told the next reader to run, so
  **run the check on the entry you are about to implement before trusting its framing**,
  especially when the entry cites a *conclusion* about another entry rather than a measurement.
- **Two builders that accept the same macro name export it twice.** A `CaptureLine` records no
  source function, so every capture builder filters the one stream by name. While the name
  tables are disjoint this is invisible; the moment a macro joins a second table, one live call
  is written into two different exported functions. Adding a name to a table is therefore a
  question about *every other* table too — grep the name across the builders before adding it,
  and if two must claim it, make one of them claim it only in the case it truly needs.
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
  auditing C1), `FLASH_SET_DIRECTION` (C3 — 8 corpus uses, so corpus frequency is no guide to
  whether a wrapper exists; check every member every time).
- **"Parsed into the IR" is not the same as "reaches the export."** The effect export is
  generated from `EffectCall`s, not from `EffectScript` statements, so a macro can have its own
  `EffectMacro` variant, parse perfectly, and still be dropped by `eval_effect_stmts` on the way
  to `EffectCall` — after which the export never sees it. A3 found `LAST_EFFECT_SET_RATE` in
  exactly that state, described in this file as already done. Before believing a surface is
  covered, follow the value all the way to the emitted text; and note that
  `cached_scripts_round_trip_through_the_emitter` only compares `(spawn_func, effect_name)`
  pairs, so it will not catch a dropped field for you.

  The general form of this was **C6**: a line with no typed variant is not merely unmodelled,
  it is *deleted* by the export. C3 found 69 more of them after A3 found 27, C5 measured the
  rest, and C6 fixed most of it — the export now copies unmodelled lines through in position,
  and 19 of 132 effect scripts still lose one rather than 28. You no longer have to assume:
  open the move in the editor and read the generated-source pane, which names both what the
  export will not write and what it copies through without understanding. C6b lists what is
  still going.

  **The root cause is worth remembering, because the other three categories do not have it.**
  A `game_` script is emitted from its statement tree, so anything in the tree survives whether
  or not the emitter understands it. The effect export is regenerated from a flat call list, so
  only what became a typed call could come out. When a new category is added (`sound_`,
  `expression_` — D1 and D2), pick the tree, not the list, and this whole family of bugs never
  starts.

  **The sharp edge: modelling a macro can make its loss quieter instead of fixing it.** C1
  gave `LAST_EFFECT_SET_COLOR` a typed variant, and 32 corpus lines promptly *stopped* being
  reported as dropped — they no longer parsed as `Raw`, so the dropped-line check could not see
  them, and they were still discarded on the way to `EffectCall` because they bind to no spawn.
  Net effect of modelling: one line recovered, thirty-two silenced. The guard is
  `EffectScript::to_effect_calls_reporting_losses`, which returns whatever the walk threw away;
  **anything that resolves a macro into a call must report what it could not resolve there**, or
  it repeats this. A test asserting the *premise* of a loss — C5's
  `a_line_the_export_cannot_reproduce_is_named_rather_than_silently_deleted` fails loudly if its
  example line becomes exportable — is what caught it.
  **The other edge of the same knife: giving a macro a variant can delete it somewhere else.**
  Code that keeps "everything we do not handle" by testing for `ExcuteStmt::Raw` is really
  testing "everything without a variant", and those stop being the same set the moment you add
  one. B4 hit this in `rebuild_script_from_hitboxes`, where dragging a hitbox silently deleted
  every `HIT_NODE` in the move. **Before modelling any macro, grep for filters on `Raw` — and
  on variant lists generally — and make them exhaustive `match`es first**, so the compiler
  forces a decision instead of defaulting to a deletion. Both halves of this trap have now cost
  real time; assume the next family has a third.
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
- [ ] `cargo test` passes (**343 green after D1a**), including the five corpus
      oracles — run
      them by name with `cargo test cached_script`, `cargo test still_loses`,
      `cargo test unbalanced` and `cargo test survives_a_round_trip`:
      `acmd_verify::tests::every_cached_script_survives_its_own_export`,
      `acmd::tests::cached_scripts_round_trip_through_the_emitter`,
      `acmd::tests::the_effect_export_still_loses_no_more_of_the_corpus_than_it_did`,
      `acmd::tests::no_game_script_in_the_corpus_exports_an_unbalanced_function` and
      `acmd::tests::every_sound_script_in_the_corpus_survives_a_round_trip`. They run
      the new code over every script the app has ever fetched (currently 461 files under
      `~/.cache/visionary/script-cache`, ~1000 functions). The third asserts a *number* — how
      many effect scripts still lose a line — so it fails on a regression rather than only on a
      crash, and the fifth pins the byte-exact count for the same reason.
      **All five return early and pass vacuously if that cache directory is missing**, and each
      carries its own guard against a corpus too thin to mean anything: `checked > 100` on three
      of them, and `branching >= 30` on the unbalanced-function one, which would otherwise stay
      green if branches simply stopped being recognised. Confirm the directory exists before
      trusting any of them. `ls` shows only 3
      entries; the files are nested per fighter, so count with
      `find ~/.cache/visionary/script-cache -type f | wc -l`.
      **Neither oracle is a full-fidelity check, so do not read a green run as one.** The
      effect one compares only `(spawn_func, effect_name)` pairs — A3's dropped rate passed it
      for as long as the bug existed. The per-field comparison lives in
      `check_effect_fidelity` / `check_hitbox_fidelity`; a new field must be added *there* or
      nothing is checking it.
      **They are also blind to a macro you have not modelled yet**, which is the trap when
      *adding* a family rather than changing one. An unparsed line exports verbatim as `Raw`, so
      it round-trips perfectly; all three were green on `WHOLE_HIT` before and after it was
      modelled. A green oracle says "nothing broke", never "the new thing works".
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
C3 (69 occurrences) beat C1 (65) and was taken first, and B4 (45) beats B3 (23). Each entry
carries its own measured counts; go by those. Blocking relationships are the only thing the
order still encodes.

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
(live capture), and `spawn_and_modifier_sites` (write-back, renamed from
`spawn_and_rate_sites` by C1 when tint and opacity joined the rate).

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

### [x] B1 — `ATTACK_ABS` (throw and swallow damage) (done 2026-08-04)

**The old title and premise were wrong, and this is the finding.** This entry called it
"absolute-position hitboxes" and said positions are world-absolute rather than bone-relative,
with a definition of done of "it draws in the viewport at the right place". Read the signature:

```rust
pub unsafe fn ATTACK_ABS(agent, kind: i32, id: u64, damage: f32, angle: u64, kbg: i32,
    fkb: i32, bkb: i32, hitlag: f32, unk: f32, facing: i32, unk2: f32, unk3: bool,
    effect: Hash40, sfx_level: i32, sfx_type: i32, _type: i32)
```

**There is no position, no size, and no bone.** It is not a spatial volume at all — it is the
damage/knockback definition applied to an opponent who is *already* caught, and the "absolute
kind" slot is what it applies to. Every one of the 32 corpus calls is in a throw or a Kirby
inhale (24 files, all `kirb*`/`doll*`), and the kinds are `..._CATCH` (22) and `..._THROW`
(15). So it cannot draw in the viewport, and the old "done when" was unreachable. The bone
dropdown does need suppressing, but for a different reason than the entry gave.

Still true from the earlier measurement: 16 arguments, all 32 occurrences, one arity, and
`macros::ATTACK_ABS` is declared — so no optional-shape detection is needed.

- **Trap, and it is the expensive one: the kind slot takes fighter-specific constants.** The
  corpus has `FIGHTER_DOLLY_ATTACK_ABSOLUTE_KIND_FINAL` alongside the two common ones. A
  `ConstTable` of just `CATCH`/`THROW` would fail to round-trip Terry's final smash, so the
  kind must be carried as written and offered as a dropdown that *accepts* unknown names
  rather than one that replaces them.
- **Slots 9, 11 and 12 are invariant across the whole corpus** (`1.0`, `0.0`, `true`) and are
  undocumented in `macros.rs` beyond `unk`/`unk2`/`unk3`. Carry them verbatim; do not expose a
  control whose meaning would be a guess. Slot 2 (`id`) is `0` in all 32.
- **Work order:** its own struct and slot table. The hit-property block genuinely does map —
  damage, angle, kbg, fkb, bkb, hitlag, `ATTACK_LR_CHECK_*`, `collision_attr`, sound level and
  attr, and `ATTACK_REGION_*` are all the same fields `ATTACK` has — so it can ride the
  existing `Hitbox` display type with geometry hidden, the way C3's colour commands ride the
  effect list. Do not reuse `ATTACK`'s *slot indices*; the layout is different.
- **Done when:** it round-trips the corpus oracles, shows in the hitbox list with geometry
  suppressed rather than zeroed, and syncs values back into the user's source.

**Two more traps found while doing it, both about the shared `ATTACK` prefix and both live
bugs rather than hypotheticals:**

- **The capture path read `ATTACK_ABS` through `ATTACK`'s 36-slot table.**
  `hitboxes_from_captures` bucketed on `func.starts_with("ATTACK")`, so a throw captured live
  had sixteen arguments read against thirty-six and came back as a plausible, wrong hitbox.
  The plugin's `is_collision_func` had the same prefix test and armed the clear-all gate for a
  call nothing clears. **Grep for `starts_with("ATTACK")` — and any other prefix bucketing —
  before adding a family whose name extends an existing one.**
- **The kind cannot be decoded back from a captured number, and must not be.** `lua_const` has
  93 `..._ATTACK_ABSOLUTE_KIND_*` constants in one namespace with heavy value collisions:
  `FIGHTER_ATTACK_ABSOLUTE_KIND_THROW` is `0x0`, and so is
  `FIGHTER_DOLLY_ATTACK_ABSOLUTE_KIND_FINAL` and most other fighters' finals. A captured `0`
  has no recoverable name. It is kept as the number, which `const_expr` writes back bare and
  which still compiles. This is the `HIT_STATUS_MASK` collision from B4 again, and worse —
  there, excluding the masks fixed it; here no table can be built at all.

Verified: 302 tests, clippy clean, `build_check.sh`, plugin builds. Both corpus oracles cover
all 32 calls. Live rules and hooks are built but not exercised against a running game.

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

### [x] B3 — Post-hoc hitbox tuning (done 2026-08-04)

These modify hitboxes *already out*, so they are edits to an existing collision rather than
new collisions.

**Measured after A3** — all have `macros.rs` wrappers, so no export trap:
`ATK_SET_SHIELD_SETOFF_MUL` 9, `ATK_HIT_ABS` 6, `WHOLE_HIT` 6, `ATK_POWER` 2,
`ATK_LERP_RATIO` 0. B4's "take that first" deferral is discharged — B4 shipped 2026-08-04.

**One member arrived late, from B5 (2026-08-04): `SET_SEARCH_SIZE_EXIST(agent, id: u64,
size: ToF32)`.** B5 listed it as a detection box; its signature says otherwise — it re-sizes a
search box *already out*, keyed on that box's id, which is precisely this entry's shape. It
would drop into `ATTACK_MOD_COMMANDS` and `AttackModKind` with no new machinery, and its
`ToF32` slot carries the same integer-formatting trap `attack_mod_num` exists for. It is
**not done**, and the reason is the one that governs B2: **zero corpus calls**, so there is
no vanilla line to prove the round trip against. Take it if a corpus with one ever appears.

**[x] `WHOLE_HIT` was not one of these and is done, as B4's family (2026-08-04).** It takes a
single `hit_status: i32` and the corpus writes `*HIT_STATUS_XLU` (5) and `*HIT_STATUS_NORMAL`
(1) — that is how the fighter *receives* hits, not an edit to a hitbox already out. It is the
all-bones sibling of `HIT_NODE`/`HIT_NO`, sharing the four-state `HIT_STATUS` `ConstTable` B4
already built. So it needed **no new `ExcuteStmt` variant** — a third `HurtTarget` alongside
`Bone` and `Group`, and all five surfaces followed from that.

- **This is the second macro filed by its name rather than its signature** (after
  `COL_PRI`/`COL_NORMAL` in B4 — see the traps section). `WHOLE_HIT` reads like a hitbox word.
  The `lua_const` oracle is **silent** for all five of B3's macros — no `MA_MSC_CMD_*`
  constants exist for them — so here the signature and the corpus context were the evidence.
- **The whole cost was one asymmetry, and it recurs on every surface.** `HIT_NODE`/`HIT_NO` are
  *(target, status)*; `WHOLE_HIT` is *(status)*, its target being the macro name. So the status
  slot index is 2 in the write-back and 1 in the capture reader for the pair, and one less for
  this one. `HurtTarget::takes_target_argument` asks it once by name. Five places needed it: the
  arity table in `HURT_COMMANDS` (listing it at 2 would have excluded every real call from
  `hurt_sites` and shifted every later site ordinal), the emitter, the source write-back, the
  capture reader, and the plugin's override application.
- **The corpus oracles cannot see this class of bug, which is worth remembering.** An unparsed
  line survives an export verbatim as `Raw`, so a `WHOLE_HIT` the parser ignored would
  round-trip exactly as cleanly as one it understood — all three oracles passed *before* the
  parse arm was added. The assertion that has teeth is that a span comes out at all.
- **The plugin had a live corruption waiting.** `hurt_action` applied `hit_target` to slot 0 and
  `hit_status` to slot 1 unconditionally, bounds-checked only. Slot 0 is the *status* for
  `WHOLE_HIT` and the *priority* for `COL_PRI`, so a rule aimed at one macro could write into
  another's meaning. It now matches on a `HurtShape` per macro. Relatedly, `COL_PRI` keyed its
  rules on a bare `u64::MAX` as "the targetless one"; there are now two targetless members, so
  the sentinels are named constants (`HURT_KEY_COL_PRI`, `HURT_KEY_WHOLE`) mirrored on both
  sides of the wire.
- **The all-bones reach is deliberately not modelled**, and the README says so. In the game a
  `WHOLE_HIT` covers the bones a `HIT_NODE` names, so it arguably ends an open per-bone span.
  No vanilla script mixes them — all 6 occurrences stand alone — so there is nothing to
  calibrate a cross-target rule against, and inventing one would draw spans the script does not
  describe. Revisit if a real mixed example turns up.

That leaves B3 proper at **17 occurrences** — but they are not one family, and the work order
below was written for a shape only two of them have. **Re-measured 2026-08-04 against
`macros.rs`, which is the oracle `lua_const` cannot be here** (it has no `MA_MSC_CMD_*` constant
for any of the four):

| macro | signature | corpus | is it an `(id, value)` modifier? |
| --- | --- | --- | --- |
| `ATK_SET_SHIELD_SETOFF_MUL` | `(id: u64, val: ToF32)` | 9 | **yes** |
| `ATK_POWER` | `(id: u64, power: ToF32)` | 2 | **yes** |
| `ATK_HIT_ABS` | `(kind: i32, unk: Hash40, target: u64, target_group: u64, target_no: u64)` | 6 | no — no id slot at all |
| `ATK_LERP_RATIO` | `(ratio: ToF32)` | 0 | no — no id slot at all |

So **B3 is scoped to the two that share the `(id, value)` shape**, 11 occurrences. The other two
are not "the same idea at a different arity"; they take no hitbox id, so the whole premise of
attaching them to a hitbox does not apply. Each is written up below with why it is not being
done rather than left to look merely unfinished.

- **The signature settled what the corpus could not.** The old bullet was right that
  `ATK_SET_SHIELD_SETOFF_MUL`'s 9 calls are byte-identical `(agent, 0, 7)` and so cannot tell the
  id slot from the value slot — but `macros.rs` declares `id: u64, val: ToF32`, which names them
  outright. `ATK_POWER` then confirms it from the corpus side: its two calls are `(agent, 0, 10)`
  and `(agent, 1, 10)`, varying the *first* slot across two hitboxes that share a value. This is
  the third time the signature was the oracle after `lua_const` came back silent.
- **Work order:** model them as modifiers attached to the hitbox id they target, shown on the
  timeline at their own frame. Do not fold their values into the parent `ATTACK` — the export
  must re-emit them as separate calls at their own frames. The corpus shows both placements:
  kirby/Attack100Sub puts `ATK_SET_SHIELD_SETOFF_MUL(agent, 0, 7)` in the *same* `is_excute`
  block as its `ATTACK(agent, 0, …)`, while kirby/AttackLw4 puts `ATK_POWER` five frames after
  the `ATTACK` it retunes. Both must survive as separate calls at their own frames.
- **Trap — the value slots are `ToF32`-generic, and the corpus writes bare integers.** All 11
  calls spell the value `7` or `10`, not `7.0`. [`acmd::num`](src/acmd.rs) deliberately appends
  `.0` because most float arguments sit in slots *declared* `f32`, where a bare `6` is a type
  error. These two are not: `ToF32` is implemented for `i32`, so both spellings compile, and
  emitting `10.0` over a vanilla `10` is gratuitous diff noise on a line the user never touched.
  Needs its own formatter, documented against `num` so the difference is not read as an
  oversight.
- **Not done — `ATK_HIT_ABS` (6), carried verbatim as `Raw` and that is the honest answer.**
  All six are the identical line `ATK_HIT_ABS(agent, *FIGHTER_ATTACK_ABSOLUTE_KIND_THROW,
  Hash40::new("throw"), target, target_group, target_no)`, where the last three are **local
  variables**, not values. There is nothing to edit: a parser expecting numbers drops the line,
  and an emitter writing numbers back breaks the throw. It is also not a modifier — with no id
  slot it is closer to B1's `ATTACK_ABS`, whose family it shares a prefix with. `game_` export
  walks the tree and keeps everything, so these already survive an export untouched.
- **Not done — `ATK_LERP_RATIO` (0).** No id slot, so it is not this family, and zero corpus
  occurrences, so it fails the same bar B2 does: the definition of done needs a round-trip test
  built from a real vanilla call and there is none. Re-measure if more of the archive is fetched.

**Result (2026-08-04).** `ATK_POWER` and `ATK_SET_SHIELD_SETOFF_MUL` land on all five surfaces as
one `ExcuteStmt::AttackMod { kind, id, value }` — one variant with a discriminant, since the two
share an argument layout exactly. 325 tests green, all 11 vanilla calls verified to round-trip
**byte-identically**.

- **Point events, not spans, and that is the whole shape of the feature.** Every other family
  here resolves to a range because some macro takes it back. Nothing takes these back, so
  `to_attack_mods` has no end-frame pass and the panel draws a marker at one frame. Inventing a
  span would have drawn a range the script never wrote — the same call B4's `WHOLE_HIT` reach
  made, arrived at from a different direction.
- **Its own site numbering space, which is the sharp part.** Reusing `HurtSite` would have meant
  that adding an `ATK_POWER` to a move shifted the ordinal of every later `HIT_NODE`, retuning a
  different call than the one clicked. `is_attack_mod_stmt` / `count_attack_mod_stmts` /
  `next_mod_site` mirror the hurtbox machinery beside it, including the zero-iteration `for`
  rewind, and `the_two_families_number_their_sites_independently` pins the boundary from both
  sides. `HurtboxAccum` became `WalkAccum` because it now carries two families.
- **A category per macro, not one keyed by hitbox id.** The two members can legally name the
  same id in the same frame window, so a shared category would let an `ATK_POWER` rule fire on an
  `ATK_SET_SHIELD_SETOFF_MUL` call and write damage into a shield multiplier. `CAT_ATK_POWER` 5
  and `CAT_ATK_SETOFF_MUL` 6 make that unrepresentable rather than merely unlikely — the same
  lesson `HURT_KEY_WHOLE` records, applied before the bug instead of after it.
- **The float formatter is a second one on purpose.** `acmd::num` appends `.0` because most
  float slots are declared `f32`, where a bare `6` is a type error. These are `ToF32`-generic and
  every vanilla call writes a bare integer, so `attack_mod_num` keeps the integer spelling and
  the write-back uses `to_f32_edit` rather than `float_edit` to match. Without this every export
  would have rewritten 11 untouched lines. **No oracle would have caught it**: `verify_move`
  checks semantics, and `7.0` means what `7` means.
- **The corpus could not have settled the slot order** — all 9 `ATK_SET_SHIELD_SETOFF_MUL` calls
  are byte-identical. `macros.rs` declaring `id: u64, val: ToF32` is what did, with `ATK_POWER`'s
  two calls confirming it by varying the first slot alone. Third time the signature was the
  oracle after `lua_const` came back silent.

### [x] B4 — Hurtbox control (done 2026-08-04)

Intangibility and hurtbox state per bone. All five members land on all five surfaces:
`HIT_NODE` 30, `COL_NORMAL` 8, `HIT_RESET_ALL` 3, `HIT_NO` 2, `COL_PRI` 2 — 45 occurrences.
Every one has a `macros.rs` wrapper at the arity the corpus writes, so no A1-style trap.

Both entry traps turned out cheaper than written. The `ConstTable` already existed with both
directions ([param_labels.rs](src/param_labels.rs)), and the panel/lane split was a clean
addition rather than a restructure. The real cost was somewhere the entry did not look.

- **The trap that actually cost time, and it is C1's again in a new place.**
  `rebuild_script_from_hitboxes` retained only `ExcuteStmt::Raw` from the source, which was
  correct *only* because every non-collision line was `Raw`. Giving `HIT_NODE` its own variant
  therefore deleted every hurtbox line in a move the moment an unrelated hitbox was dragged.
  The filter is now an exhaustive `match` so the next variant is a compile error instead of a
  silent deletion. **Generalise this: before modelling any macro, grep for every place that
  filters on `Raw` or on a variant list, and make it exhaustive first.**
- **`HIT_STATUS_MASK_*` collides with `HIT_STATUS_*` numerically.** `MASK_NORMAL` is `0x1` and
  so is `INVINCIBLE`. `const_name` returns the first match, so a table built by prefix would
  label a live capture of an invincible bone as a mask. The table is exactly the four real
  states; `TERM` and the masks are excluded and the doc comment says why.
- **Design, and it differs from collisions on purpose:** hurtbox statements are *carried
  through* `state.script` rather than rebuilt from an edited list. The script is the model, so
  there is no second copy to drift, and `HurtboxState::site` is the ordinal that ties a
  resolved span back to the statement it came from. `hurt_stmt_mut` and `HurtboxAccum` are
  written to reproduce the same pre-order walk — including stepping the cursor over a `for`
  body whose count is zero, which is the one case where the two definitions could diverge.
- **Out of scope, deliberately:** the panel edits existing calls only. There is no "add a
  hurtbox state" button, and moving one to a different frame is not offered. Both are
  structure, which the export path can express and the source sync cannot; adding them means
  deciding where a new call goes in a block, which is the same open question as C6. A retime
  or a target-family swap made some other way is *reported*, not guessed.
- Verified: 296 tests, clippy clean, `build_check.sh`, plugin builds (2318336 bytes). The
  corpus oracle now runs `check_hurtbox_fidelity` over all 461 cached scripts, so every vanilla
  hurtbox call is known to re-export identically. The live surface is built but not exercised
  against a running game — that needs a deploy, which is not available here.

### [x] B5 — `SEARCH` / detection boxes (done 2026-08-04)

Grab-range and detection volumes. Geometrically these are close to grab boxes, so the panel
work is mostly reuse.

**Measured after A3:** `SEARCH` 7; `SET_SEARCH_SIZE_EXIST`, `ENABLE_AREA`, and `UNABLE_AREA`
appear **zero** times in the local corpus, though all four have `macros.rs` wrappers. So this
is really a one-macro task with three untestable extras — there is no vanilla call to build a
round-trip test from for those three, which the definition of done requires. Scope it to
`SEARCH` and say so, rather than shipping three families no corpus can check.

**Re-measured on the way in (2026-08-04), and the zero-corpus count was not the only reason
to drop the three extras — their signatures were.** Reading each, the way B3 should have been
read:

- `ENABLE_AREA(agent, kind: i32)` and `UNABLE_AREA(agent, kind: i32)` take **one int and no
  geometry at all**. They toggle an area the stage or the fighter already owns; they do not
  define a volume. "Geometrically close to grab boxes" cannot describe them — they are not a
  smaller `SEARCH`, they are a different thing, and they belong with the enable/disable
  lifetime commands (near **C4**), not in a collision panel. Filed there rather than here.
- `SET_SEARCH_SIZE_EXIST(agent, id: u64, size: ToF32)` re-sizes a search box that is *already
  out*. That is not a box command, it is the exact shape **B3** just built: a post-hoc
  modifier keyed on the id of a live collision. It also has the `ToF32` slot B3 found, so it
  carries the same integer-formatting trap. It belongs in B3's table, gated on a corpus call
  existing to test it against — which there is not. Recorded under B3, not done here.

So B5 is one macro, `SEARCH`, with 7 vanilla calls across 7 kirby scripts. The corpus does
exercise both arities (4 calls without the capsule end, 3 with it) and the extras genuinely
vary — `COLLISION_KIND_MASK_ATTACK`/`_HIT`, `HIT_STATUS_MASK_ALL`/`_NORMAL`, and an unnamed
int that is 0, 1 and 60 — so keeping them losslessly is doing real work, not defending
against a hypothetical.

`SEARCH` maps onto `Hitbox` better than `ATTACK_ABS` did: id, part, bone, size, x/y/z, the
optional capsule end, and all three of `situation_mask`/`category_mask`/`part_mask` land on
fields that already exist. The four with no home — collision kind, hit status, the unnamed
int, and a trailing bool — become a `SearchExtras` beside `CatchExtras` and `AbsExtras`.

- **Trap:** new collision family → it must be added to the plugin's `is_collision_func` and
  given a category id on the wire, alongside 0 attack / 1 grab / 2 wind.

**Result (done 2026-08-04).** All five surfaces. 335 tests, clippy clean, `cargo fmt --check`
clean, `build_check.sh` exit 0, plugin builds and the binary contains `hook_search` and the
`ATTACK/CATCH/SEARCH/WIND/CLEAR/HURT/ATKMOD` banner. Corpus at 460 files, so the oracles are
not vacuous. The live surface is built but not exercised against a running game.

- **Half the trap above was wrong, and reading the function said so.** `is_collision_func` is
  not "is this a collision" — it arms a gate that notes something is *out to be cleared*. A
  search volume is never cleared, exactly as `ATTACK_ABS` is not, and that comment is already
  in the function explaining why `ATTACK_ABS` is excluded. Adding `SEARCH` would have made the
  next `AttackModule::clear_all` look like it ended something. It is deliberately not added.
- **`SEARCH` is written in two shapes and the tail moves three slots between them.** Four of
  the seven vanilla calls omit the capsule arguments outright rather than writing `None`, so
  the mask arguments sit at 11..=13 in one and 8..=10 in the other. Each of the four surfaces
  needs its own discriminator, and **they are not the same test**: in source text a capsule
  slot is visibly a coordinate and the next argument is visibly a `*CONST`, but on the wire
  both are just numbers, so the live reader has to go by *argument count*. Testing the value
  there gave a 14-argument call a capsule of `[2.0, 1.0, 60.0]` — its own collision kind, hit
  status and undocumented int, bent into geometry. A test caught it; nothing else would have.
- **Two real bugs found on the way in, both older than this task.** Written up below.
- Not done, deliberately: `SET_SEARCH_SIZE_EXIST` (moved to **B3**), `ENABLE_AREA` and
  `UNABLE_AREA` (moved to **C4**). None has a corpus call to test against.

### [x] B5a — The short-form `CATCH` bug B5 surfaced (done 2026-08-04)

Not a planned entry; found while measuring B5, because `CATCH` is dumped in the same two shapes
`SEARCH` is. Both halves read the status kind and situation mask from fixed slots 10 and 11.

- **Parsing** ran off the end of the token list on the short form and substituted the defaults,
  so all four short-form calls in the corpus — every one of them Kirby's inhale — came into the
  editor as an ordinary `CAPTURE_PULLED` grab instead of a swallow.
- **Writing** was worse: a capsule edit put `Some(1.0)` over the status constant, destroying the
  grab's behaviour and producing a file that does not compile. Now refused and reported.

**Why no oracle caught it, which is the part worth keeping.** `check_hitbox_fidelity` compares
the export against the *parsed model*, not against the original text — so a value lost on the
way in agrees with itself on the way out and the round trip is green. Both new tests assert
against the original text instead. Any future check of this kind needs the same shape.

### [x] B5b — Live rules were sent under the editor's category, not the plugin's (done 2026-08-04)

Also found on the way in, and it is why B5's live surface could not have been correct without
fixing it first.

The editor put `Hitbox.category` straight onto the wire. That works for attack, grab and wind,
where the two numbering spaces agree, and then stops: the editor's `CAT_ABS` is **3**, which is
the plugin's `CAT_HURT`. So **every live `ATTACK_ABS` edit since B1 has been read as a hurtbox
rule and silently done nothing** — the plugin listens for those on 4. A search box (display 4)
would have collided with `ATTACK_ABS` (wire 4) the same way.

Fixed with an explicit `game_link::wire_category()` that every rule-push site now goes through,
plus a test pinning each mapping and asserting neither family lands on the hurtbox category.

- **The lesson is the one this file keeps relearning, in a new place.** Two id spaces that
  agree on their first few members are not the same space, and the agreement is what hides it.
  It is the same shape as the site-ordinal counters in B3 and the per-family slot tables at the
  top of this file: *sharing a numbering space across families is the recurring bug here.*
- **Not verified against a running game.** The mapping is proven by test; that a live `ATTACK_ABS`
  edit now actually lands needs a deploy, which is not available here.

## Effects

### [x] C1 — `LAST_EFFECT_SET_COLOR` and `LAST_EFFECT_SET_ALPHA`

Done 2026-08-04. All five surfaces; `cargo test` 282 green, both corpus oracles run for real
(461 files present), clippy clean, `build_check.sh` passes. The plugin was touched and builds;
the `diag.txt` stamp could not be checked, because `scripts/build.sh` only writes to
`target/output/` and deploying needs a running game.

Scoped on the way in to the two members with real corpus backing left after A3 took rate. Both
arities are uniform — `(agent, r, g, b)` 65 times and `(agent, a)` 4 times, no exceptions — and
both are declared in `macros.rs`. The remaining three are **C7**, below.

**The premise was wrong about where the value is, and that is the finding.** The entry said 33
of the 65 colour calls were being deleted by exports and that C1 would recover them. Modelling
the macro recovers **one**. Measured after the fact over the whole cache: `tint` binds on 1 call,
`alpha` on 4, and **64 of the 65 colour lines bind to nothing.** They look like this, and this
shape is nearly the whole corpus:

```rust
if macros::is_excute(agent) {
    macros::EFFECT_FOLLOW_ALPHA(agent, Hash40::new("dolly_roll_l_color1"), ...);
}
if(0x2508e0(*FIGHTER_INSTANCE_WORK_ID_INT_COLOR, 0)){
    if macros::is_excute(agent) {
        macros::LAST_EFFECT_SET_COLOR(agent, 0.146, 0.205, 0.333);
    }
}
```

A costume check between the spawn and its recolour. At runtime the tint does apply — the game's
"last effect" outlives the block — but it applies **on one costume**, so binding it would export
a costume-specific colour as an unconditional one. Refusing is correct, and the fix is not a
looser anchor. It is **C6**: carry the conditional through. Do not "fix" this by reaching further
back; A3 settled that rule and it is still right.

**The near-miss worth knowing about, because the next family will hit it too.** Modelling a
macro moved 32 lines *out* of C5's dropped-line report without moving them into the export: they
stopped parsing as `Raw`, so `unexportable_effect_lines` no longer saw them, and they were
discarded on the way to `EffectCall` with nothing anywhere saying so. Modelling a macro made the
loss quieter. `EffectScript::to_effect_calls_reporting_losses` now returns the modifiers that
bound to nothing, from the same walk that resolves the calls, so there is one implementation of
the anchor rule and no way to drop a line without naming it. `check_dropped_lines` reports both
kinds together. **Any future `LAST_EFFECT_SET_*` must return its unbound lines there** or it will
repeat this exactly.

**What did land, and is worth having regardless of the corpus count:**

- **The tweak-versus-script conflict is resolved.** `emit_effect_move_fn` was already writing
  `LAST_EFFECT_SET_COLOR` for a live colour multiplier while the parser had no variant to read
  one back — so a script's own tint was deleted and a tweak's survived, invisibly, because both
  sides of `check_effect_fidelity`'s comparison had already lost the line. Now the two reconcile
  the way A3 made rate reconcile: the override wins, exactly one line is written, and the
  verifier warns that it replaced the script's.
- **Panel, live, and write-back** are coherent for both macros. The plugin hooks
  `LAST_EFFECT_SET_COLOR` / `_ALPHA` for capture and override (33 hooks now, was 31), the wire
  carries `tint` / `alpha` as `Option` + `skip_serializing_if`, and `spawn_and_rate_sites` became
  `spawn_and_modifier_sites` — one scan for all three, sharing `modifier_edits`.
- **A modifier no longer ends the run for the modifier after it.** A tint followed by a rate is
  two lines about one spawn; the parser, the capture reconstruction, and the write-back scanner
  were each changed to agree on that, and `MODIFIER_COMMANDS` in
  [acmd_src.rs](src/acmd_src.rs) is the list to extend when a fourth arrives.

**Deliberate non-goals:**

- The tweak's fourth colour component is still ignored on export. It is the live form's alpha,
  which no panel exposes and which `live_tweak_from_override` does not test for identity, so
  emitting it would ship an opacity the user never set. Surfacing it is a live-override task,
  not this one.
- `num` for the tint and alpha slots, not the bare `to_string` the rate uses. Every one of the 65
  colour calls in the archive is written with a decimal point while its rates are whole numbers;
  matching each macro's own spelling is what keeps a re-exported vanilla script textually
  identical to its source.

### [ ] C7 — The last four `LAST_EFFECT_SET_*` members

What C1 left. Corpus counts and wrapper status, all verified against the full `macros.rs`
declaration list:

| macro | args | corpus | `macros.rs` wrapper |
|---|---|---|---|
| `LAST_PARTICLE_SET_COLOR` | 3 (`ToF32`) | 1 | yes |
| `LAST_EFFECT_SET_WORK_INT` | — | 1 | **NO** |
| `LAST_EFFECT_SET_SCALE_W` | 3 (`ToF32`) | 0 | yes |
| `LAST_EFFECT_SET_OFFSET_TO_CAMERA_FLAT` | 1 (`ToF32`) | 0 | yes |

**This is a low-value entry and should probably stay open forever.** Two of the four appear
nowhere in the corpus, and the other two appear once each. C1's result is the argument against
doing it: modelling a whole family recovered five real calls. Take it only if a user asks for
one of these by name.

- `LAST_PARTICLE_SET_COLOR` targets the last *particle*, not the last effect. It is not a fourth
  member of C1's family and must not share its anchor — check what the game binds it to before
  modelling it at all.
- **`LAST_EFFECT_SET_WORK_INT` is the `AREA_WIND_2ND` situation again**: `sv_animcmd` has it, the
  archive uses it once, and `macros.rs` does not declare it. If it is modelled it must be
  parse-only, with a verifier blocker on export — the same shape as `WIND_MACRO_COMMANDS` /
  `has_macro_wrapper` in [data.rs](src/data.rs).
- Whatever is added, return its unbound lines from `to_effect_calls_reporting_losses`. See C1.

### [~] C2 — Sword trails

`AFTER_IMAGE4_ON` / `_arg29` / `AFTER_IMAGE_ON`. Read-only today: the trail shows on the
timeline with its graphic and joint, and write-back explicitly refuses it
([acmd_src.rs:803](src/acmd_src.rs:803)) because a trail has no transform.

That refusal is correct and must stay. But a trail *is* placed — by the joints it names,
arguments 4 onward. Those are editable; the transform never will be.

- **Work order:** expose the joint pair and the trail parameters. Keep refusing transform
  writes, with the existing message.
- **Done when:** changing a trail's joints rewrites those arguments and nothing else, and
  dragging a position still reports the existing "no position arguments" skip.

**Measured on the way in (2026-08-04). The joint half cannot be done, and measuring found a
worse problem underneath it.**

- **Every trail-ON macro has zero corpus calls.** `AFTER_IMAGE4_ON` 0, `AFTER_IMAGE4_ON_arg29`
  0, `AFTER_IMAGE_ON` 0. Only `AFTER_IMAGE_OFF` appears, 4 times. So there is no vanilla call to
  build the round-trip test the definition of done requires — the same bar **B2** fails on.
- **Two of the three names do not exist.** `smash-script` declares only
  `AFTER_IMAGE4_ON_arg29`, `AFTER_IMAGE4_ON_WORK_arg29` and `AFTER_IMAGE_OFF`. There is no
  `macros::AFTER_IMAGE4_ON` and no `macros::AFTER_IMAGE_ON`, so an export emitting either names
  a function that does not exist — exactly the `AREA_WIND_2ND` trap `WIND_MACRO_COMMANDS`
  already documents. The parser accepting those names is *fine* (a source file may contain
  them and they ride through verbatim); synthesising one would not be.
- **The `_arg29` fixture in the tests is fabricated and has the wrong shape.** The real
  signature is 29 arguments with the second joint at slot **8**, not slot 5, and slots 5..=7
  are `trail_x1/y1/z1`. The fixture writes a `Hash40` where a coordinate goes. Any joint-pair
  work built on that fixture would have been built on a call the game never makes. **Do not
  reuse it — replace it if this is ever picked up with a real call in hand.**

So the joint pair is deferred, not refused: it is a reasonable feature with no way to verify it
today. Take it if a corpus with a real trail call ever appears, and read the signature first.

### [x] C2a — `AFTER_IMAGE_OFF` is exported without its argument (done 2026-08-04)

Found by the measurement above, and this one *is* testable — against 4 real corpus lines.

`AFTER_IMAGE_OFF<F: ToF32>(agent, unk: F)` takes one argument. The corpus writes
`macros::AFTER_IMAGE_OFF(agent, 0);` twice and `(agent, 3);` twice. `emit_spawn_stop` in
[acmd.rs:1940](src/acmd.rs:1940) emits `macros::AFTER_IMAGE_OFF(agent);` — **no argument at
all** — so every exported effect script containing a sword trail is a project that does not
build. The existing test asserts the wrong output, which is why it was never noticed.

- Why no oracle caught it: no corpus script pairs a trail ON with an OFF (there are zero ONs),
  so the emitter branch is never reached by the corpus round-trip. Coverage, not blindness.
- The `ToF32` slot carries B3's formatting trap: the corpus writes bare `0` and `3`, so this
  must not emit `0.0`. Reuse `attack_mod_num`.
- The author's own value has to survive, so `EffectMacro::AfterImageOff` needs to carry it.
  For a trail the editor ended itself there is no donor and a default is needed; the corpus
  splits 2/2 between `0` and `3`, so the default is a genuine choice and should say so.

**Result.** `EffectMacro::AfterImageOff` now carries `arg`, `EffectCall` carries `trail_off`,
and the emitter writes `macros::AFTER_IMAGE_OFF(agent, N);` through `attack_mod_num`.
`TRAIL_OFF_DEFAULT` is `0` and its doc comment says outright that it is a choice, not a
measurement. 337 tests, clippy clean, `cargo fmt --check` clean, `build_check.sh` exit 0.

- Three tests, each mutation-checked: reverting the formatter to `num` fails all three. The
  round-trip test covers **both** corpus values rather than one, because the archive does not
  agree on this argument and a test pinning only `0` would have let `3` be normalised away.
- **The old test asserted the broken output.** That is the finding worth carrying: a test can
  pin a bug as firmly as it pins a behaviour, and this one made the export look verified. When
  a fixture is hand-written rather than lifted from the corpus, it is evidence of nothing —
  and this file's fixture was hand-written *and* structurally impossible (see C2 above).
- Not a regression from any recent task; the bare call has been emitted since trails were
  first modelled. It survived because no corpus script pairs a trail ON with an OFF, so the
  round-trip oracle never reaches the branch. **Coverage, not oracle blindness** — the distinct
  failure mode from B5a, which the oracle ran over and still could not see.

### [x] C3 — Screen and body colour effects

Done 2026-08-04. All five surfaces; `cargo test` 273 green, both corpus oracles run for real
(461 files present). The plugin was touched and builds; the `diag.txt` stamp could not be
checked, because `scripts/build.sh` only writes to `target/output/` and deploying needs a
running game.

**The premise held — 69 occurrences, all six wrapped, the panel really is mostly a colour
picker — but the entry described the wrong problem.** This was filed as coverage: a family the
editor could not edit. It was worse than that. The effect export regenerates the whole function
from `EffectCall`s, and these parsed as `Raw`, so **every export silently deleted them**. A
mod built from kirby/AttackDash shipped with the burn colouring gone and nothing anywhere said
so. Same shape as A3's dropped rate, four times the usage, and the entry did not predict it
because it was reasoning about the panel.

**The measured argument layout, which is uniform across the family and was not obvious:**

| macro | args | corpus | shape |
|---|---|---|---|
| `FLASH` | 4 | 41 | r, g, b, a |
| `BURN_COLOR` | 4 | 10 | r, g, b, blend |
| `BURN_COLOR_FRAME` | 5 | 10 | **frames**, then the same four |
| `BURN_COLOR_NORMAL` | 0 | 5 | reset |
| `START_INFO_FLASH_EYE` | 0 | 3 | — |
| `FLASH_FRM` | 5 | 0 | **frames**, then the same four |

One layout for all six: the interpolation length comes first where there is one, and the four
colour components follow. That is measured, not assumed — the corpus pairs
`BURN_COLOR(agent, 2, 0.059, 0.008, 0)` with
`BURN_COLOR_FRAME(agent, 4, 2, 0.059, 0.008, 0.9)` on the very next line, which is the same
four values with a length pushed in front. So `color_slots` is one function rather than six
tables, and B1's warning about per-family slot tables does not bite here.

**`FLASH_SET_DIRECTION` is a third instance of the A1 trap, and is deliberately not modelled.**
`sv_animcmd` has it, the corpus calls it 8 times (dolly `SpecialHiCommand` and
`SpecialAirHiCommand`), and `macros.rs` does **not** wrap it. Modelling it would mean either
emitting a macro that does not exist or blocking the export of two Terry moves that export
today. It stays an unmodelled line — which means it is still dropped on export, exactly as it
is now. See C5.

**How they are carried, and why:** a colour command is an `EffectCall` with `color: Some(..)`
and the command in `spawn_func`, sharing the effect list rather than getting one of its own.
Everything that list already does — reordering, disabling, undo, project save, write-back
ordinals, export grouping by frame — is what these need too, and a parallel list would be a
second copy of all of it to keep in step. The cost is that the spawn fields are meaningless on
such an entry, so every site that reads one checks `color` first. The sites that had to learn
this: `emit_spawn_call`, `export_spawn_downgrades`, `check_effect_values`,
`effect_call_display_name`, `is_spawn_macro`, `transform_matches`, the properties panel, and
`push_effect_rules` — where `effect_name_hash("")` would otherwise have keyed every colour
command in the game to one hash.

**Also landed:**

- **Live.** The plugin hooks all six: it records them, so a captured move comes back with its
  colouring, and rewrites their arguments when a rule retunes one. These name no effect kind,
  so a rule keys `eff_hash` on hash40 of the lowercased *command* name — no graphic is called
  `burn_color` — which reuses the whole motion/frame/suppress matcher unchanged. `color` and
  `transition` are `Option` + `skip_serializing_if`, so older plugin builds ignore them.
- **A command dropdown**, the same shape as A2's. Switching changes how many arguments the call
  takes, so the payload is reshaped to the new signature on the spot rather than left for
  `check_color_values` to block on.
- `check_color_values` refuses a payload whose shape disagrees with its command — that is a
  call the signature does not accept, so it is a blocker by the same rule as a wind command an
  argument short.

**Known gaps, deliberately left:**

- The live path can retune a command the script already calls; it cannot inject one the script
  never makes, and cannot move one to a different frame. Unlike a spawn there is no captured
  argument list to replay. Adding and retiming reach the export, not the preview.
- The colour picker clamps to 0..=1 but the drag fields do not, because the corpus writes
  `BURN_COLOR(agent, 2, …)` — an over-bright red a clamping editor would silently dim.

### [x] C5 — Name the lines the effect export deletes

Done 2026-08-04. `cargo test` 275 green, both corpus oracles run for real, clippy clean,
`build_check.sh` passes. Plugin untouched — this is verifier-only, and surfaces 1–3 and 5 are
unchanged by design: the point is to describe what surface 4 already does, not to change it.

Split out of C3, which found the general case behind its own symptom. Scoped down on the way
in to the *report* half only; the preservation half is now **C6**, below.

The effect export regenerates the function from `EffectCall`s, so any line without a typed
variant is deleted on export. That was true before C3, before A3, and is still true — the
change here is that it is no longer silent. [`unexportable_effect_lines`](src/acmd.rs) walks an
`EffectScript` for `Raw` statements and `Raw` macros, and `check_dropped_lines` in
[acmd_verify.rs](src/acmd_verify.rs) names each one under the generated-source pane.

**What the corpus actually loses.** Measured over the script cache, not estimated — 132 effect
scripts produce calls, and **32 of them (24%) lose at least one line.** By head of line:

| Count | Line | Note |
|---|---|---|
| 33 | `macros::LAST_EFFECT_SET_COLOR` | The biggest single loss. **C1 modelled it and this barely moved:** all but one of these are cut off from their spawn by a costume `if`, so they now bind to nothing and are reported as unbound rather than as unparsed. **C6 closed this to 0** — carried verbatim under its own costume check. |
| 62 | `if` / `if !WorkModule::is_flag` / `if get_value_float` / `else {` | Conditional effects export as unconditional. **C6 closed this to 5**, all of them nested guards. It also fixed a second bug hiding underneath: the parser flattened `if A { X } else { Y }` into sibling spawns, so a move exported as doing *both*. |
| 7 | `wait_loop_sync_mot` | A timing primitive with no `EffectStmt` variant. |
| 4 | `macros::LAST_EFFECT_SET_ALPHA` | **Fixed by C1** — all four bind and survive an export. |
| 4 | `methodlib::L2CAgent::pop…` | Not a call to model; genuinely script plumbing. |
| 3 | `EffectModule::req_screen…` | Direct module calls, no macro wrapper involved. |
| 2 ea | `FILL_SCREEN_MODEL_COLOR`, `CANCEL_FILL_SCREEN` | Screen-wide colour, adjacent to C3 but a different family — **this line was right and C6b's later bullet contradicted it**; both are E3's, at arity 12 and 2. |
| 1 ea | `LAST_EFFECT_SET_WORK_INT`, `COL_NORMAL`, two `EffectModule` calls | `LAST_EFFECT_SET_WORK_INT` is the A1 trap again (see C7). |

**Warning, not blocker — and that is the decision to revisit if it ever looks wrong.** A real
loss of the user's code arguably belongs in classes 1–3, which refuse the export. It is a
warning because 24% of vanilla scripts carry such a line: blocking would swap a lossy export
for no export, which helps nobody, and the message a user acts on is identical either way.
**C6 has since landed and the residue is 19 of 132, not 32.** The arithmetic behind this
paragraph is therefore out of date; C6b carries the re-decision. Do not re-quote the 24% figure.

- **Known gap, deliberate:** the **Export Mod Folder** path passes `None` and does not run this
  check. A saved project stores `effect_calls_full` — resolved `EffectCall`s and nothing else —
  so by export time the dropped lines are already gone from the data. Making the project carry
  them is the same plumbing C6 needs, so it lands there rather than being done twice. The
  generated-source pane, which is where a user looks before exporting, does have the script and
  does check. **Half closed by C6:** carried lines ride on the calls a project saves, so those
  do report here. The dropped half is C6c.
- **Also deliberate:** punctuation-only lines are filtered out. The emitter regenerates every
  brace it needs, so reporting one `}` per block would bury the lines that are a loss. The
  filter is "contains a letter or digit", which keeps `if … {` and `else {` in.

### [x] C6 — Carry unmodelled effect lines through the export (done 2026-08-04)

Done. `cargo test` 305 green, both corpus oracles run for real plus a new third one, clippy
clean, `build_check.sh` exit 0. Plugin untouched — see the live gap below.

**Result: the export drops lines from 19 of 132 effect scripts, down from 28.** Measured, not
estimated, and now asserted by `the_effect_export_still_loses_no_more_of_the_corpus_than_it_did`
so the number can only go down. C5's table said 32; the true baseline on `main` was 28, because
C1 and C3 had already moved some. What the remaining 19 lose:

| Was | Now | Line |
|---|---|---|
| 64 | **0** | costume `if(0x2508e0(…)){` headers |
| 33 | **0** | `LAST_EFFECT_SET_COLOR` — every one now carried under its own costume check |
| 12 | **0** | `if !WorkModule::is_flag(…) {` |
| 9 | **0** | `if get_value_float(… LR) < 0.0 {` |
| 8 | **0** | `FLASH_SET_DIRECTION` |
| 9 | 5 | `else {` — the survivors are inside a second conditional, reported by design |
| 8 | 8 | `COL_NORMAL` — in blocks with no spawn to ride on |
| 7 | 7 | `wait_loop_sync_mot` — **deliberate**, see below |
| 4 | 2 | `methodlib::L2CAgent::pop()` |

**The root cause, which C5 did not name.** The two exports are not built the same way.
[`emit_move_fn`](src/acmd.rs) emits a `game_` script from `script.stmts` — it walks the
statement tree, so anything in the tree survives whether or not the emitter understands it.
[`emit_effect_move_fn`](src/acmd.rs) regenerated an `effect_` script from a flat
`Vec<EffectCall>` grouped by frame, so **only what became a typed call could come out**. Every
loss in C5's table followed from that one asymmetry.

**Measured the corpus before designing** (`137` effect functions), because the shape of the
residue decides whether to fix the asymmetry or work around it:

| Measurement | Result | What it ruled out |
|---|---|---|
| Functions with a non-`is_excute` conditional | **13 of 137** | A narrow feature. Not worth rewriting the emitter for. |
| Maximum conditional nesting depth | **1** | No recursion. A guard is one string, not a stack. |
| `frame()` / `wait()` inside a conditional | **1** | Frame arithmetic does not branch, so frame-grouping survives. |
| Statements inside a guard | **only `is_excute` blocks and effect macros** | A guard always wraps whole blocks, so it can be re-emitted around one. |
| Brace-balanced function bodies | **137 of 137** | Verbatim passthrough of a block is safe. |

So the frame-grouped emitter stayed and learned to carry residue. A structure-preserving rewrite
would have put add, delete and retime — the editor's actual value — at risk to serve 13 files.

**What landed.** `EffectStmt::Cond` (the parser stops flattening conditionals),
`EffectCall::guard` (a spawn re-exports inside its own condition), and
`EffectCall::leading` / `trailing` (verbatim lines, wrapper regenerated). Panel shows them under
**Kept as written**; `check_carried_lines` warns that a copied line is not an understood one.

**Findings worth keeping:**

- **Position is load-bearing, so residue hangs off a call and not off a frame.**
  `LAST_EFFECT_SET_COLOR` recolours whatever spawned most recently, and dolly's `SpecialHiCommand`
  puts three spawns and 24 tints in one frame block. Anchoring residue to the frame would have
  emitted the spawns then the tints and landed all 24 on the third spawn — every line present,
  move wrong. Pinned by `each_costume_tint_stays_with_the_spawn_it_recolours`.
- **Flattening was hiding a second bug.** The parser turned `if A { spawn X } else { spawn Y }`
  into two sibling spawns, so a move that spawned one graphic facing left and another facing
  right exported as spawning *both*. `Cond` fixes that as a side effect of keeping the header.
- **`else` is not `else`.** In the dumps it attaches to `if macros::is_excute(agent)`, not to the
  outer conditional — [kirby/CapturePulledHi.txt:6](). The decompiler mis-scopes it and the
  function balances only by accident. A guard header is reproduced as opaque text and never
  interpreted; anything pairing an `else` with its `if` would be wrong on real input.
- **Loop unrolling duplicated the loss report** — `eval_effect_stmts` runs a `for` body once per
  iteration, so a line inside one was named four times. `COL_NORMAL` read 16 against a true 8
  until the walk started rewinding its residue state per iteration, exactly as
  `call_macro_ordinals` already did. Caught by comparing against a worktree at `main` rather than
  trusting the new number.
- **The doubling trap cannot fire.** C6's entry warned that re-emitting a preserved line that is
  itself an unrecognised spawn would double it. It cannot: a line only becomes residue if it
  produced no `EffectCall`, so a carried line and a regenerated call are never the same spawn.
  Asserted in `a_rate_with_no_spawn_directly_above_it_attaches_to_nothing`.
- **Timing primitives are deliberately still dropped.** `wait_loop_sync_mot` advances the
  coroutine, and the regenerated function states every frame absolutely with its own `frame()`
  calls. Carrying it would shift every effect after it — an export that compiles and plays wrong,
  which is worse than an honest deletion. All 7 stay reported.
- **Faithful preservation means preserving invalid Rust.** The dumps are decompiler output:
  `if(0x2508e0(…)){` will not compile. Carrying it through is still the right trade — a user can
  fix a line they can see, not one that was deleted — but it turns a silent wrong export into a
  build error, so `check_carried_lines` says so up front rather than letting `cargo` say it.

**Known gaps, deliberately left:**

- **Live preview does not carry these.** A carried line is text whose condition only the game can
  evaluate; the plugin replays captured calls and a capture already reflects the branch that ran.
  Export and the generated-source pane are the surfaces this task touches. The plugin is
  unchanged, and not by oversight.
- **C5's export-path gap is now half closed rather than closed.** Carried lines travel *on* the
  calls, so a saved project reports them without needing the script. Dropped lines still do not:
  that would need `FighterEdits` to store the loss list beside `effect_calls_full` — a schema
  change for a report, not for behaviour. See [acmd_verify.rs:161](src/acmd_verify.rs:161).
- **One guard deep.** A conditional inside a conditional would have to overwrite the outer one,
  and neither choice is right — keeping the outer is too permissive, keeping the inner is wrong
  in a different direction. Nothing in the corpus nests, so the outer is kept and the inner is
  reported as a loss. That is the 5 remaining `else {`.
- **Guarded spawns are stopped unguarded.** `EFFECT_OFF_KIND` is `kill_kind`, a no-op when
  nothing of that kind is live, so an unguarded stop for a guarded spawn is harmless — whereas a
  guarded stop would leave the effect running forever on the branch that skipped the guard.

### [~] C6b — The effect scripts the export still loses a line from

C6 took the corpus from 28 lossy scripts to 19 and asserted the number, so this is what is
left. Read C6's result table first — it says exactly which lines these are. Nothing here is
large; the point of the entry is that the remainder has been *identified*, so it can be closed
deliberately rather than discovered again.

**`COL_NORMAL` is done (2026-08-04): 19 → 15, all 8 occurrences recovered.** The rest of this
entry is still open.

- **[x] `COL_NORMAL`, 8 occurrences — the biggest item, and the cheapest.** These sat in
  `is_excute` blocks that contain no spawn, so there was no call for them to ride on.

  **The entry's premise was wrong, and checking it first is what made this a one-row change.**
  `COL_PRI` and `COL_NORMAL` are *colour-blend* commands, not body collision: `lua_const` names
  them `MA_MSC_CMD_COLOR_BLEND_COL_PRI` and `MA_MSC_CMD_COLOR_BLEND_COL_NORMAL`, in a family of
  exactly six with `FLASH`, `FLASH_FRM` and `FLASH_OFF`. So `COL_NORMAL` is not merely "the same
  shape as C3" — it is C3's family, and the direct sibling of the `BURN_COLOR_NORMAL` already in
  `COLOR_COMMANDS`. One row in that table, no new type, and the panel, the emitter and the
  write-back all picked it up untouched because two argument-free colour commands already shipped
  through them.

  What it actually cost beyond that row:
  - **A plugin hook move, not an addition.** `sv_animcmd::COL_NORMAL` was *already* hooked, in
    `hitbox_viewer`, next to `COL_PRI` and recording only. Only one hook may replace a symbol, so
    the second one failed to link — which is the good failure. It now lives with the colour
    family in `effect_viewer::acmd_hooks`, records identically, and additionally honours
    suppression, so disabling a reset live is what it should be: the tint stays up.
  - **A duplication a `CaptureLine` cannot resolve.** Capture lines do not record which function
    ran them, so every builder filters the one stream by macro name — and two builders that
    accept the same name export it into two different functions off one live call. The tables
    happened to be disjoint until now. `hurtbox_script_from_captures` therefore takes
    `COL_NORMAL` only while a `COL_PRI` it saw is open, which is the one thing that side still
    needs it for. **Trap inside the trap:** the open/closed flag must be moved by that pair
    alone. Reading it off every statement lets the `HIT_RESET_ALL` that so often shares the reset's
    frame close the span first, and the existing capture test catches exactly that.
  - **`COL_PRI` deliberately left alone.** It is not in the loss list — both occurrences share an
    `is_excute` block with a `FLASH`, so C6 already carries them as that call's `leading`. Folding
    it into `COLOR_COMMANDS` needs a third payload shape, since it takes one integer rather than a
    transition or an RGBA. Do it only if something else forces a `ColorCall` field anyway.
  - **The `game_` side is now correctly *labelled* but still oddly *placed*.** All ten corpus
    occurrences of the pair are in `effect_` functions, so `ExcuteStmt::ColPri` / `ColNormal` has
    no corpus backing at all. Its plumbing is fine and the panel no longer calls the value a
    pushbox priority, but it still sits under hurtboxes. Moving it is a UI decision, not a
    correctness one — left for whoever next touches that panel.
- **`else {`, 5 occurrences — nested guards.** C6 keeps one guard per spawn and reports an inner
  one rather than overwriting the outer. Closing this means `EffectWalk::guard` becoming a stack
  and `EffectCall::guard` a `Vec<String>`. Cheap, but there is **no corpus case that exercises
  it correctly** — these 5 are all the mis-scoped `else` described in C6, so a test would be
  pinning decompiler noise. Do it only alongside a real nested example.
- **`wait_loop_sync_mot`, 7 — do not "fix" this.** It is dropped by decision, not by omission:
  it advances the coroutine while the regenerated function states every frame absolutely, so
  carrying it shifts every effect after it. The honest close is to *model* it as a timing
  statement that the frame walk understands, which is E2's neighbourhood (`FT_MOTION_RATE`),
  not this one.
- **`methodlib::L2CAgent::pop()`, 2, and the bare `EffectModule::remove_screen` calls, 2.**
  Genuine script plumbing with no editor meaning. Worth leaving reported.
- **`CANCEL_FILL_SCREEN`, 2 — belongs to E3, not here. This bullet was wrong on both counts**
  (written 2026-08-04, corrected the same day by the arity check it told itself to do).
  It *is* wrapped in `macros.rs`, so it is emittable — that part held. But:
  - **C3 does not model `FILL_SCREEN_MODEL_COLOR`.** C6's own result table already said
    "adjacent to C3 but a different family", and this bullet contradicted it from memory.
    Nothing under `src/` touches the name. There is no family here to add a reset to.
  - **It is not `COLOR_COMMANDS`-shaped.** `CANCEL_FILL_SCREEN` takes `(i32, f32)` and
    `FILL_SCREEN_MODEL_COLOR` takes **twelve** arguments including an `EffectScreenLayer` and a
    screen priority. Neither is a transition-plus-RGBA, so a row in that table cannot hold them.
  - **The `lua_const` oracle is silent here.** There is no `MA_MSC_CMD_*_FILL_SCREEN` constant at
    all — only an unrelated `MA_MSC_EFFECT_FILL_SCREEN_LEGACY`. Worth knowing that the oracle
    answers for some macros and not others; when it does not, the signature plus the corpus
    context is the evidence.

  All 4 occurrences are in dolly's `FinalAirStart` / `FinalAirEnd` — full-screen final-smash
  staging, which is exactly what E3 describes, and E3 already lists both macros by name. Do it
  there, with the camera work, or not at all.
- **Then reconsider** whether the C5 warning should become a blocker. C6 and C6b both changed the
  arithmetic behind that decision: it was a warning because a quarter of vanilla scripts tripped
  it, and now under one in eight does, of which half are the deliberate `wait_loop_sync_mot`.
  Still probably a warning — but the reasoning in C5's entry is now out of date and should be
  re-derived rather than re-quoted.

**Measured remainder after `COL_NORMAL`, 15 of 132 scripts:** `wait_loop_sync_mot` 7, `else {` 5,
`methodlib::L2CAgent::pop()` 2, `CANCEL_FILL_SCREEN` 2, `EffectModule::remove_screen` 2.

**Nothing cheap is left in this entry.** Every remaining line is now either deliberate
(`wait_loop_sync_mot`), untestable against this corpus (`else {`), genuine plumbing with no
editor meaning (`pop()`, `remove_screen`), or another entry's work (`CANCEL_FILL_SCREEN` → E3).
C6b should be closed at 15 rather than kept open for a cheap win that does not exist — the only
thing still owed is the warning-vs-blocker re-derivation below, which is a decision, not code.

### [ ] C6c — Close C5's export-path gap

Carried lines already report on the export path, because they travel on the `EffectCall`s a
project saves. Dropped lines do not: nothing in a saved project remembers them.

- Store the loss list beside `effect_calls_full` in `FighterEdits`, populate it where the script
  is still in hand, and pass it at [acmd_verify.rs:161](src/acmd_verify.rs:161).
- **This is a schema change for a report, not for behaviour** — which is why C6 left it. Weigh
  that before starting: the generated-source pane, which is where a user looks before exporting,
  already has the script and already checks. The gap only bites someone who exports a saved
  project without opening the pane.

### [ ] C4 — Effect lifetime control

Detach interacts with the follow/off-kind lifetime the editor already models for spawns.

**Not schedulable as written — every remaining member has zero corpus calls.**

**Re-measured 2026-08-05: `SET_PLAY_INHIVIT` is not an effect command and never was.** Its
signature is `(agent, se: Hash40, unk: ToF32)`, its 10 corpus arguments are all sound-effect
labels (`se_kirby_dash_stop`, `se_common_dizzy`), and **all 10 calls sit inside `sound_`
functions.** It suppresses a sound effect, not an effect spawn. It belongs to D1 and has been
counted there. That leaves this entry with `EFFECT_DETACH_KIND` 0, `EFFECT_DETACH_KIND_WORK` 0,
`ENABLE_AREA` 0 and `UNABLE_AREA` 0 — nothing to test a round trip against.

The earlier "measured after A3" line counted `SET_PLAY_INHIVIT` without reading its signature,
which is the trap two entries above this one warns about, applied to an entry's *own* evidence.
A count says a macro is used; only the signature says what it is.

**Inherited from B5 (2026-08-04):** `ENABLE_AREA(agent, kind: i32)` and
`UNABLE_AREA(agent, kind: i32)` were filed under B5 as detection volumes. They take one int
and no geometry — they turn an existing area on and off, which is this entry's subject, not
B5's. Both have **zero** corpus calls, so they are subject to the same round-trip-test bar
the detach half fails; do not schedule them on their own.

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

**Measured 2026-08-05.** Sound is the largest unmodelled thing left in the corpus by a wide
margin — `PLAY_SE` alone has **429 calls**, against 386 for `ATTACK`. Counting the family:
`PLAY_SE` 429, `STOP_SE` 41, `PLAY_STEP_FLIPPABLE` 38, `PLAY_SEQUENCE` 28, `PLAY_SE_REMAIN` 21,
`PLAY_STATUS` 15, `PLAY_LANDING_SE` 12, `PLAY_DOWN_SE` 11, `SET_PLAY_INHIVIT` 10,
`PLAY_FLY_VOICE` 5 — about 610 calls across 301 of the 460 corpus files. If "largest task" was
the reason to defer it, "largest coverage gap" is the reason to take it.

- **Work order, in this order, each landing on its own:**
  1. **[x] D1a (done 2026-08-05)** — Load `sound_` in `script_body` and parse it to `Raw` only,
     with a corpus round-trip gate.
  2. **[~] D1b (in progress)** — Merge project and mirror *per category*, which is what D1a
     named as step 2's own first move. Then type the `PLAY_SE` family and give sound a
     timeline lane.
  3. Plugin hooks for the sound primitives, capture, then live.
  4. Export + write-back. Note the generated plugin installs per category, so a `sound_` script
     it does not emit still plays vanilla — nothing is lost by sound being absent until here.
- **Trap:** the moment a *write* path lands, a file that was previously never rewritten becomes
  rewritable. The corpus round-trip is the gate for that, not a spot check.

#### [x] D1a — Load and re-emit `sound_` scripts (done 2026-08-05)

**Result:** `extract_function(source, prefix)` replaces the two copy-pasted extractors;
`parse_sound_script` / `emit_sound_body` read and write the body. Both are staged — the
corpus gate is their only caller, deliberately: the generated-source pane promises what an
export would write, and step 4 is where an export starts writing sound.

`every_sound_script_in_the_corpus_survives_a_round_trip` asserts three properties over all
**301** sound scripts, rather than the byte-equality the entry originally asked for. See the
trap about malformed sources for why. 295 are byte-exact; the other 6 differ only in the
indentation the dumper got wrong.

**Two pre-existing bugs the gate surfaced on its first run, both older than this entry and
neither about sound:**

- **A runtime branch lost the brace that closes it.** `parse_stmts` kept `if …{` as a one-line
  `Raw` and skipped every lone `}`. **35 of 236 `game_` scripts** exported a function with two
  more `{` than `}`, and an `if`/`else` had both arms promoted to unconditional — Kirby's Final
  Smash start issued `FT_START_CUTIN` twice. Fixed with `AcmdStmt::RawBlock`, which keeps the
  header, the walked body and the brace. `no_game_script_in_the_corpus_exports_an_unbalanced_function`
  is the standing gate; it asserts at least 30 scripts actually branch so it cannot pass
  vacuously.
- **A body with no `game_` function was read whole by the game parser.** Every line of an
  effect-only or sound-only move came back as `Raw`, and `emit_stmts` writes `Raw` into the
  generated `game_` function — so its effects would spawn twice, once from each script. The
  fallback now applies only to a body with no ACMD function header at all, which is the live
  capture and paste case it exists for.

**Deliberately unchanged:** a hitbox inside a branch is still walked as though the branch always
runs, which is what happened when branches were flattened. 18 corpus `game_` scripts place an
`ATTACK` this way and would lose it from the editor otherwise. The fix here was to the *brace*,
not to the condition; pinned by `a_hitbox_inside_a_branch_is_still_seen_by_the_frame_walk`.

#### [~] D1b — Merge the project and the mirror per category

D1a named this as a sound problem. It is not: it is an *every category* problem, and it is
already live for the most common mod shape there is.

`script_body` ([acmd_src.rs:312](src/acmd_src.rs:312)) concatenates whatever categories the
project defines and returns `Some`. `fetch_acmd` ([app.rs:1723](src/app.rs:1723)) treats
`Some` as the whole answer and drops the mirror fetch on the floor — including any fetch
already in flight. So a project that overrides only `game_attackairn`, which is what most
hitbox mods are, displays the move **with no effects at all**, and the editor cannot tell
that from a move that genuinely has none. The sound-only case is the same bug seen from a
different side.

- **The fix:** resolve each of the four categories independently — project's if it has one,
  mirror's otherwise — and concatenate the result.
- **The thing to get right is not the merge, it is the fetch.** The project path is
  deliberately inline and synchronous ("it is a local file read"). A partial override now
  needs the mirror too, so: use the disk cache when it is warm, spawn the existing worker
  when it is not, and carry the project's own parts through to the poll site so they still
  win on arrival. **A mirror fetch that fails must fall back to project-only** — offline
  must not be worse than today.
- **Test bar:** a project defining only `game_` keeps vanilla's effects; only `effect_` keeps
  vanilla's hitboxes; only `sound_` keeps both. A project category always beats the mirror's.

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
([acmd_verify.rs:941](src/acmd_verify.rs:941)) — the editor does not model animation rate, and
a timing warning that guesses is worse than none. Modelling rate would re-enable those checks
for a large slice of the corpus.

**Blocked 2026-08-05 on a fact that cannot be established offline: which way the multiplier
goes.** 17 corpus calls, all Kirby, all top-level, values 0.25–1.0. The two readings give
opposite answers and both have evidence:

- *Rate is playback speed*, so `game = motion / rate`. `FT_MOTION_RATE(0.5)` halves the speed.
  This is the community reading and what "rate" normally means.
- *Rate is game frames per motion frame*, so `game = rate × motion`. This is what
  `FT_MOTION_RATE_RANGE`'s own arithmetic says — it computes
  `rate = game_frames / (motion_end_frame - motion_start_frame)`, which only makes sense if a
  rate above 1 stretches a span.

Under the first reading Kirby's jab 1 hitbox lands on game frame 5; under the second, frame 2.
Nothing on this machine distinguishes them: the smashline docs say only that rate "changes the
speed of the animation and how fast the script playback is", and the corpus cannot be used as
an oracle because it contains no independent statement of when a move actually hits.

**Do not take this entry until the direction is settled**, and settle it by *measuring*, not by
reasoning: the plugin already reports `MotionModule::frame`, so capturing a rate-carrying move
live and comparing the reported frame against the script frame answers it in one run. Getting
it backwards is worse than today's "not modelled" — the entry's own done-when says so.

- **Also unmodelled and inherited from the same reading:** a `game_` script's rate calls change
  the pacing of that move's `effect_` and `sound_` scripts too. Whatever the timeline does with
  rate has to apply across all four categories, not just the one the call is written in.
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
